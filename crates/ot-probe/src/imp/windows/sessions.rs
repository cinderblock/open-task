//! Logon sessions, from the terminal services API.
//!
//! `WTSEnumerateSessionsW` on the local server returns every session with its station
//! name and connect state, the services session 0 included. `WTSQuerySessionInformationW`
//! then gives each session's user, domain and client machine; every one of those
//! answers is a buffer the API allocates and this module frees. Sessions change
//! rarely (a sign-in, an RDP connect), and one enumeration is not cheap: about 5 ms
//! on this machine with two sessions (measured by the ignored `time_one_enumeration`
//! test), nearly all of it the per-session queries, each a round trip into the
//! terminal services service. So the enumeration runs at most every
//! [`ENUMERATE_EVERY`] and [`SessionProbe::sample`] hands back the cached list in
//! between.
//!
//! Entries are matched by session id across enumerations and the caller's vector is
//! refilled in place, so neither path allocates in steady state beyond the API's own
//! buffers. Account names follow [`super::details`]: `DOMAIN\user`, or the bare name
//! when the domain is this computer or `NT AUTHORITY`.
//!
//! Listener sessions (`RDP-Tcp` waiting for a connection, state `WTSListen`) are not
//! sessions anybody is in and are left out unless a user is attached to one.
//!
//! [`disconnect`] and [`logoff`] act on a session by id. Both are allowed on the
//! session open-task itself runs in: disconnecting the console session just drops to
//! the lock screen, and logging it off ends open-task with everything else. Neither is
//! refused here; the UI decides what to offer.

use std::mem;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use ot_model::session::{SessionInfo, SessionState};
use windows::core::PWSTR;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CTX_WINSTATION_NOT_FOUND};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSActive, WTSClientName, WTSDisconnectSession, WTSDisconnected,
    WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory, WTSListen, WTSLogoffSession,
    WTSQuerySessionInformationW, WTSUserName, WTS_INFO_CLASS, WTS_SESSION_INFOW,
};
use windows::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
use windows::Win32::System::Threading::GetCurrentProcessId;

use crate::ControlError;

/// How often the session list is worked out again. A sign-in or an RDP connection
/// shows up within this long.
const ENUMERATE_EVERY: Duration = Duration::from_secs(5);

/// Lists logon sessions. One per probe.
#[derive(Debug)]
pub(super) struct SessionProbe {
    /// `NetBIOS` name of this machine, left off local account names.
    computer: String,
    /// The session this process runs in, when the OS would say.
    current: Option<u32>,
    /// The last enumeration, sorted by session id.
    listed: Vec<SessionInfo>,
    /// Emptied vector kept for the next enumeration so its capacity is reused.
    spare: Vec<SessionInfo>,
    enumerated_at: Option<Instant>,
    /// Scratch for decoding the API's strings.
    name: String,
    domain: String,
    /// Set while enumeration fails, so the warning is logged once per outage.
    enumerate_failing: bool,
    /// Set once a per-session query has failed, so that is warned about once.
    query_warned: bool,
}

impl SessionProbe {
    pub fn new() -> Self {
        let mut id = 0u32;
        // SAFETY: valid out-pointer; our own PID always exists.
        let current = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut id) }
            .ok()
            .map(|()| id);
        if current.is_none() {
            tracing::warn!("ProcessIdToSessionId failed; no session is marked current");
        }
        Self {
            computer: computer_name(),
            current,
            listed: Vec::new(),
            spare: Vec::new(),
            enumerated_at: None,
            name: String::new(),
            domain: String::new(),
            enumerate_failing: false,
            query_warned: false,
        }
    }

    /// Refill `out` with every session, sorted by id. Between enumerations this is a
    /// copy of the last result.
    pub fn sample(&mut self, out: &mut Vec<SessionInfo>) {
        let now = Instant::now();
        if self
            .enumerated_at
            .is_none_or(|t| now.duration_since(t) >= ENUMERATE_EVERY)
        {
            self.enumerate();
            self.enumerated_at = Some(now);
        }
        copy_into(out, &self.listed);
    }

    /// Ask the terminal services service for the session list. On failure the last
    /// list is kept, so a transient error does not blank the UI.
    fn enumerate(&mut self) {
        let mut ptr: *mut WTS_SESSION_INFOW = null_mut();
        let mut count = 0u32;
        // SAFETY: the server handle is the local server (null); both out-pointers are
        // valid locals; the guard frees the array the API allocates.
        let r = unsafe { WTSEnumerateSessionsW(None, 0, 1, &raw mut ptr, &raw mut count) };
        if let Err(e) = r {
            if !self.enumerate_failing {
                tracing::warn!(error = %e, "WTSEnumerateSessionsW failed; session list kept");
            }
            self.enumerate_failing = true;
            return;
        }
        self.enumerate_failing = false;
        let list = Enumeration {
            ptr,
            count: count as usize,
        };

        let mut next = mem::take(&mut self.spare);
        for s in list.entries() {
            let id = s.SessionId;
            // Reuse the previous entry for this id so its strings keep their buffers.
            let mut info = match self.listed.iter().position(|l| l.id == id) {
                Some(i) => self.listed.swap_remove(i),
                None => SessionInfo {
                    id,
                    user: None,
                    station: String::new(),
                    state: SessionState::Other,
                    client: None,
                    current: false,
                },
            };

            self.name.clear();
            self.domain.clear();
            if let Some(user) = self.query(id, WTSUserName) {
                push_wide(&mut self.name, user.as_wide());
            }
            if self.name.is_empty() {
                info.user = None;
            } else {
                if let Some(domain) = self.query(id, WTSDomainName) {
                    push_wide(&mut self.domain, domain.as_wide());
                }
                let local = self.domain.is_empty()
                    || self.domain.eq_ignore_ascii_case("NT AUTHORITY")
                    || self.domain.eq_ignore_ascii_case(&self.computer);
                let user = info.user.get_or_insert_with(String::new);
                user.clear();
                if !local {
                    user.push_str(&self.domain);
                    user.push('\\');
                }
                user.push_str(&self.name);
            }
            if s.State == WTSListen && info.user.is_none() {
                continue;
            }

            info.station.clear();
            if !s.pWinStationName.is_null() {
                // SAFETY: the API's station names are NUL-terminated and live in the
                // enumeration array, which the guard keeps alive for this loop.
                push_wide(&mut info.station, unsafe { s.pWinStationName.as_wide() });
            }

            // A session nobody is signed in to is idle whatever the OS calls it: the
            // services session reports WTSDisconnected and the logon screen WTSActive.
            info.state = if info.user.is_none() {
                SessionState::Idle
            } else if s.State == WTSActive {
                SessionState::Active
            } else if s.State == WTSDisconnected {
                SessionState::Disconnected
            } else {
                SessionState::Other
            };

            self.name.clear();
            if let Some(client) = self.query(id, WTSClientName) {
                push_wide(&mut self.name, client.as_wide());
            }
            set_opt(&mut info.client, &self.name);

            info.current = self.current == Some(id);
            next.push(info);
        }
        next.sort_by_key(|s| s.id);
        // Whatever is left in `listed` belonged to sessions that are gone.
        self.listed.clear();
        self.spare = mem::replace(&mut self.listed, next);
    }

    /// One string attribute of a session. `None` when the API will not say, which
    /// is logged once.
    fn query(&mut self, id: u32, class: WTS_INFO_CLASS) -> Option<WtsString> {
        let mut p = PWSTR::null();
        let mut bytes = 0u32;
        // SAFETY: local server; valid out-pointers; the guard frees the buffer.
        let r = unsafe { WTSQuerySessionInformationW(None, id, class, &raw mut p, &raw mut bytes) };
        match r {
            Ok(()) => Some(WtsString(p)),
            Err(e) => {
                if !self.query_warned {
                    tracing::warn!(error = %e, session = id, "WTSQuerySessionInformationW failed");
                    self.query_warned = true;
                }
                None
            }
        }
    }
}

/// Disconnect a session, leaving its programs running. Returns without waiting for
/// the session to finish tearing down.
///
/// # Errors
/// `NotPermitted` for another user's session when not elevated; `Gone` for a
/// session id that no longer exists.
pub(super) fn disconnect(id: u32) -> Result<(), ControlError> {
    // SAFETY: plain call on the local server; no wait, so the caller's thread is
    // not held while the session tears down.
    unsafe { WTSDisconnectSession(None, id, false) }.map_err(|e| map("WTSDisconnectSession", e))
}

/// Sign a session out, ending its programs. Returns without waiting for the logoff
/// to finish.
///
/// # Errors
/// As [`disconnect`].
pub(super) fn logoff(id: u32) -> Result<(), ControlError> {
    // SAFETY: plain call on the local server; no wait.
    unsafe { WTSLogoffSession(None, id, false) }.map_err(|e| map("WTSLogoffSession", e))
}

fn map(context: &'static str, e: windows::core::Error) -> ControlError {
    match e.code() {
        c if c == ERROR_ACCESS_DENIED.to_hresult() => ControlError::NotPermitted,
        c if c == ERROR_CTX_WINSTATION_NOT_FOUND.to_hresult() => ControlError::Gone,
        _ => ControlError::os(context, e),
    }
}

/// The array `WTSEnumerateSessionsW` allocates; freed on drop.
struct Enumeration {
    ptr: *mut WTS_SESSION_INFOW,
    count: usize,
}

impl Enumeration {
    fn entries(&self) -> &[WTS_SESSION_INFOW] {
        if self.ptr.is_null() || self.count == 0 {
            return &[];
        }
        // SAFETY: the API wrote `count` structures at `ptr`; they live until the
        // guard frees them.
        unsafe { std::slice::from_raw_parts(self.ptr, self.count) }
    }
}

impl Drop for Enumeration {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: the pointer came from WTSEnumerateSessionsW and is freed once.
            unsafe { WTSFreeMemory(self.ptr.cast()) }
        }
    }
}

/// A string `WTSQuerySessionInformationW` allocates; freed on drop.
struct WtsString(PWSTR);

impl WtsString {
    fn as_wide(&self) -> &[u16] {
        if self.0.is_null() {
            return &[];
        }
        // SAFETY: the API's string answers are NUL-terminated; the buffer lives
        // until the guard frees it.
        unsafe { self.0.as_wide() }
    }
}

impl Drop for WtsString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from WTSQuerySessionInformationW and is freed
            // once.
            unsafe { WTSFreeMemory(self.0 .0.cast()) }
        }
    }
}

/// Append UTF-16 to `dst`, replacing bad surrogates, without allocating when there
/// is room.
fn push_wide(dst: &mut String, wide: &[u16]) {
    dst.extend(
        char::decode_utf16(wide.iter().copied()).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)),
    );
}

/// Set an optional string, keeping its buffer when it stays `Some`.
fn set_opt(dst: &mut Option<String>, src: &str) {
    if src.is_empty() {
        *dst = None;
    } else {
        let s = dst.get_or_insert_with(String::new);
        s.clear();
        s.push_str(src);
    }
}

/// Make `out` equal to `from`, reusing its entries' strings.
fn copy_into(out: &mut Vec<SessionInfo>, from: &[SessionInfo]) {
    out.truncate(from.len());
    for (dst, src) in out.iter_mut().zip(from) {
        dst.id = src.id;
        dst.user.clone_from(&src.user);
        dst.station.clone_from(&src.station);
        dst.state = src.state;
        dst.client.clone_from(&src.client);
        dst.current = src.current;
    }
    out.extend(from[out.len()..].iter().cloned());
}

fn computer_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and length agree.
    let r = unsafe {
        GetComputerNameExW(
            ComputerNameNetBIOS,
            Some(PWSTR(buf.as_mut_ptr())),
            &raw mut len,
        )
    };
    match r {
        Ok(()) => String::from_utf16_lossy(&buf[..len as usize]),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_session_is_active_with_a_user_and_services_is_session_0() {
        let mut p = SessionProbe::new();
        let mut out = Vec::new();
        p.sample(&mut out);
        assert!(!out.is_empty(), "no sessions listed");
        assert!(
            out.windows(2).all(|w| w[0].id < w[1].id),
            "not sorted by id"
        );

        let current: Vec<_> = out.iter().filter(|s| s.current).collect();
        assert_eq!(
            current.len(),
            1,
            "expected exactly one current session: {out:?}"
        );
        let me = current[0];
        assert_eq!(me.state, SessionState::Active);
        assert!(me.user.as_deref().is_some_and(|u| !u.is_empty()), "{me:?}");
        assert_ne!(me.station, "");

        let services = out.iter().find(|s| s.id == 0).expect("session 0 listed");
        assert_eq!(services.station, "Services");
        assert_eq!(services.user, None);
        assert_eq!(services.state, SessionState::Idle);
        assert!(!services.current);

        // No listener slipped through: every entry without a user is Idle, and a
        // listener never has a user.
        for s in &out {
            if s.user.is_none() {
                assert_eq!(s.state, SessionState::Idle, "{s:?}");
            }
        }
    }

    #[test]
    fn a_second_sample_within_the_interval_is_the_cached_list() {
        let mut p = SessionProbe::new();
        let mut first = Vec::new();
        p.sample(&mut first);
        let at = p.enumerated_at;
        let mut second = vec![SessionInfo {
            id: u32::MAX,
            user: Some("stale".into()),
            station: "stale".into(),
            state: SessionState::Other,
            client: Some("stale".into()),
            current: true,
        }];
        p.sample(&mut second);
        assert_eq!(p.enumerated_at, at, "enumerated again within the interval");
        assert_eq!(first, second);
    }

    #[test]
    fn logging_off_a_session_that_cannot_exist_fails_without_panicking() {
        let r = logoff(u32::MAX - 1);
        eprintln!("logoff of a non-existent session: {r:?}");
        assert!(matches!(
            r,
            Err(ControlError::Gone | ControlError::Os { .. } | ControlError::NotPermitted)
        ));
    }

    #[test]
    #[ignore = "timing; run by hand to refresh the number in the module doc"]
    fn time_one_enumeration() {
        let mut p = SessionProbe::new();
        p.enumerate();
        let n = 200;
        let start = Instant::now();
        for _ in 0..n {
            p.enumerate();
        }
        let each = start.elapsed() / n;
        eprintln!("one enumeration of {} sessions: {each:?}", p.listed.len());
        for s in &p.listed {
            eprintln!("  {s:?}");
        }
    }
}
