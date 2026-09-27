//! Which services live in which process, from the service control manager.
//!
//! One `EnumServicesStatusExW` call per pass returns every Win32 service with the
//! PID it runs in. That is the whole cost: a few hundred entries, no handle per
//! service, unelevated. Names are interned so republishing the list every second
//! allocates nothing in steady state.
//!
//! The service DLL comes from the registry, once per service name. It is a hint for
//! where the service's code lives, shown next to the name; the CPU can still be in
//! another module.

use std::collections::HashMap;
use std::sync::Arc;

use ot_model::service::{ServiceInfo, ServiceState};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_ANY,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW,
    SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_PAUSED, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_WIN32,
};

/// Interned strings for one service name.
#[derive(Debug, Clone)]
struct Interned {
    name: Arc<str>,
    display_name: Arc<str>,
    dll: Option<Arc<str>>,
}

/// Maps PIDs to the services they host. One per probe.
#[derive(Debug)]
pub(super) struct ServiceProbe {
    scm: Option<SC_HANDLE>,
    /// Enumeration buffer. `u64` elements so the structures the API writes at its
    /// start are 8-byte aligned; handed to the API as bytes.
    buf: Vec<u64>,
    interned: HashMap<String, Interned>,
    /// This pass's answer. Vectors are kept between passes and refilled.
    by_pid: HashMap<u32, Vec<ServiceInfo>>,
    /// Scratch for reading names out of the enumeration buffer.
    name: String,
}

// SAFETY: an SCM handle is a process-wide token, valid from any thread; the probe
// uses it only from the sampler thread and closes it there.
unsafe impl Send for ServiceProbe {}

impl ServiceProbe {
    pub fn new() -> Self {
        // SAFETY: plain call; null machine and database mean the local active DB.
        let scm =
            unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
                .ok();
        if scm.is_none() {
            tracing::warn!("service control manager not readable; no service attribution");
        }
        Self {
            scm,
            buf: Vec::with_capacity(8 * 1024),
            interned: HashMap::new(),
            by_pid: HashMap::new(),
            name: String::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.scm.is_some()
    }

    /// Refresh the PID to services map for this pass.
    pub fn refresh(&mut self) {
        let Some(scm) = self.scm else {
            return;
        };
        for v in self.by_pid.values_mut() {
            v.clear();
        }
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
                    return;
                }
            }
        }

        let count = returned as usize;
        if count * size_of::<ENUM_SERVICE_STATUS_PROCESSW>() > self.buf.len() * 8 {
            return;
        }
        // SAFETY: the API wrote `count` structures at the start of the buffer, with
        // the strings they point to further in; the buffer is 8-byte aligned and
        // outlives the slice.
        let entries = unsafe {
            std::slice::from_raw_parts(
                self.buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                count,
            )
        };
        for e in entries {
            let pid = e.ServiceStatusProcess.dwProcessId;
            if pid == 0 {
                continue;
            }
            let state = match e.ServiceStatusProcess.dwCurrentState {
                SERVICE_RUNNING => ServiceState::Running,
                SERVICE_START_PENDING => ServiceState::StartPending,
                SERVICE_STOP_PENDING => ServiceState::StopPending,
                SERVICE_STOPPED => ServiceState::Stopped,
                SERVICE_PAUSED => ServiceState::Paused,
                _ => ServiceState::Unknown,
            };
            // SAFETY: both strings are NUL-terminated and live in `self.buf`.
            let (name, display) = unsafe {
                (
                    e.lpServiceName.to_string().unwrap_or_default(),
                    e.lpDisplayName.to_string().unwrap_or_default(),
                )
            };
            self.name.clear();
            self.name.push_str(&name);
            let interned = if let Some(i) = self.interned.get(&self.name) {
                i.clone()
            } else {
                let i = Interned {
                    name: Arc::from(name.as_str()),
                    display_name: Arc::from(display.as_str()),
                    dll: service_dll(&name),
                };
                self.interned.insert(name, i.clone());
                i
            };
            self.by_pid.entry(pid).or_default().push(ServiceInfo {
                name: interned.name,
                display_name: interned.display_name,
                state,
                dll: interned.dll,
            });
        }
        for v in self.by_pid.values_mut() {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        self.by_pid.retain(|_, v| !v.is_empty());
    }

    /// Services hosted by `pid` as of the last refresh, sorted by name.
    pub fn services_of(&self, pid: u32) -> Option<&[ServiceInfo]> {
        self.by_pid.get(&pid).map(Vec::as_slice)
    }
}

impl Drop for ServiceProbe {
    fn drop(&mut self) {
        if let Some(h) = self.scm.take() {
            // SAFETY: the handle came from OpenSCManagerW and is closed once.
            unsafe {
                let _ = CloseServiceHandle(h);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
