//! Per-process details that need a process handle: image path, command line, user
//! and integrity level.
//!
//! `NtQuerySystemInformation` reports every process's counters without a handle to
//! any of them. The rest needs `OpenProcess`. We ask for
//! `PROCESS_QUERY_LIMITED_INFORMATION`, the weakest right: a process grants it to its
//! own user, and protected processes grant it where they refuse everything else. When
//! even that is refused (another user's process, seen from an unelevated app) the
//! image path still comes from `SystemProcessIdInformation`, which needs no handle,
//! mapped from its NT device path to a drive letter. The user and command line stay
//! unknown in that case rather than guessed.
//!
//! The architecture comes from `IsWow64Process2` on the same handle, and the
//! description and company from the image's version resource ([`verinfo`]), read
//! from the path, so they are known for a process that refused a handle too.
//!
//! Everything here runs once per process lifetime, under the per-pass time budget the
//! probe enforces. Account names are cached by SID: a machine has a handful of
//! distinct users, and `LookupAccountSidW` can go to a domain controller. Version
//! strings are cached by path, since a dozen processes often share one image.
//!
//! Efficiency mode is the one fact here that changes while a process runs, so it
//! has its own reader, [`efficiency_mode_of`], which the probe calls on a slow
//! round-robin rather than once.

use std::collections::HashMap;
use std::mem::size_of;
use std::time::{Duration, Instant};

use ot_model::process::{Architecture, Integrity, ProcessStatic};
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Wdk::System::SystemInformation::NtQuerySystemInformation;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, STATUS_BUFFER_OVERFLOW,
    STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH, STATUS_SUCCESS, UNICODE_STRING,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetLengthSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
    LookupAccountSidW, TokenIntegrityLevel, TokenUser, PSID, SECURITY_MAX_SID_SIZE,
    SID_AND_ATTRIBUTES, SID_NAME_USE, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};
use windows::Win32::System::SystemInformation::{
    ComputerNameNetBIOS, GetComputerNameExW, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64,
    IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_ARMNT, IMAGE_FILE_MACHINE_I386,
    IMAGE_FILE_MACHINE_UNKNOWN,
};
use windows::Win32::System::Threading::{
    GetProcessInformation, IsWow64Process2, OpenProcess, OpenProcessToken, ProcessPowerThrottling,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::nt::{SystemProcessIdInformation, SYSTEM_PROCESS_ID_INFORMATION};
use super::verinfo::VerInfoCache;
use super::{unicode_to_string, AlignedBuf};

/// What one query learned. `None` means "could not be read"; the UI shows it blank
/// and the probe never asks again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Details {
    pub image_path: Option<String>,
    pub command_line: Option<String>,
    pub user: Option<String>,
    pub integrity: Integrity,
    pub architecture: Architecture,
    pub description: Option<String>,
    pub company: Option<String>,
}

impl Details {
    pub fn apply(self, s: &mut ProcessStatic) {
        s.image_path = self.image_path;
        s.command_line = self.command_line;
        s.user = self.user;
        s.integrity = self.integrity;
        s.architecture = self.architecture;
        s.description = self.description;
        s.company = self.company;
    }
}

/// Whether the process is under execution-speed power throttling (efficiency
/// mode), read from a handle with `PROCESS_QUERY_LIMITED_INFORMATION`, which is
/// enough: the `own_process_reads_its_efficiency_mode` test opens with exactly
/// that right. `None` when the call fails (a process from before the API, or a
/// protected one).
///
/// The state has two masks: `ControlMask` says which policies are set explicitly
/// and `StateMask` which of those are on. Both set means on; the control bit
/// alone means explicitly off; neither means the system default, which is off.
pub(super) fn efficiency_mode(h: HANDLE) -> Option<bool> {
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    // SAFETY: `state` is a valid out-struct of the size passed.
    unsafe {
        GetProcessInformation(
            h,
            ProcessPowerThrottling,
            (&raw mut state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .ok()?;
    let on = state.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
        && state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0;
    Some(on)
}

/// [`efficiency_mode`] by PID: one `OpenProcess` with the limited right. `None`
/// when the process cannot be opened.
pub(super) fn efficiency_mode_of(pid: u32) -> Option<bool> {
    if pid == 0 || pid == 4 {
        return None;
    }
    // SAFETY: plain call; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let eco = efficiency_mode(h);
    // SAFETY: `h` came from OpenProcess and is closed exactly once.
    unsafe {
        let _ = CloseHandle(h);
    }
    eco
}

/// The instruction set a process runs, from `IsWow64Process2`: the process
/// machine is `UNKNOWN` for a native process, in which case the native machine is
/// the answer; otherwise it is the emulated one (`I386` under WOW64, `ARMNT` or
/// `AMD64` on ARM64 hardware).
fn architecture(h: HANDLE) -> Architecture {
    let mut process = IMAGE_FILE_MACHINE_UNKNOWN;
    let mut native = IMAGE_FILE_MACHINE_UNKNOWN;
    // SAFETY: two valid out-pointers.
    if unsafe { IsWow64Process2(h, &raw mut process, Some(&raw mut native)) }.is_err() {
        return Architecture::Unknown;
    }
    let machine = if process == IMAGE_FILE_MACHINE_UNKNOWN {
        native
    } else {
        process
    };
    architecture_of(machine)
}

fn architecture_of(machine: IMAGE_FILE_MACHINE) -> Architecture {
    match machine {
        IMAGE_FILE_MACHINE_AMD64 => Architecture::X64,
        IMAGE_FILE_MACHINE_I386 => Architecture::X86,
        IMAGE_FILE_MACHINE_ARM64 => Architecture::Arm64,
        IMAGE_FILE_MACHINE_ARMNT => Architecture::Arm,
        _ => Architecture::Unknown,
    }
}

const MAX_SID: usize = SECURITY_MAX_SID_SIZE as usize;

/// A SID copied out of a token buffer so it stays valid while the buffer is reused.
#[derive(Debug, Clone, Copy)]
struct Sid {
    bytes: [u8; MAX_SID],
}

impl Sid {
    fn as_psid(&self) -> PSID {
        PSID(self.bytes.as_ptr().cast_mut().cast())
    }
}

/// Integrity levels are the last sub-authority of the token's mandatory-label SID,
/// `S-1-16-<level>`. Anything below medium (an `AppContainer`, a sandboxed renderer)
/// counts as low.
const MANDATORY_MEDIUM_RID: u32 = 0x2000;
const MANDATORY_HIGH_RID: u32 = 0x3000;
const MANDATORY_SYSTEM_RID: u32 = 0x4000;

/// Longest path we will ask for, in UTF-16 units; `\\?\` paths can reach this.
const MAX_WIDE_PATH: usize = 32_768;
/// Largest command line we will read. Windows caps the command line at 32K
/// characters; the buffer also holds the `UNICODE_STRING` header.
const MAX_COMMAND_LINE_BYTES: usize = MAX_WIDE_PATH * 2 + 64;
/// Drive-letter mappings are refreshed at most this often, and only when a path
/// fails to map (a volume mounted after startup).
const DRIVE_REFRESH: Duration = Duration::from_secs(30);

/// Collects details. One per probe, reused across passes, so steady state allocates
/// only for the strings it hands out.
#[derive(Debug)]
pub(super) struct DetailProbe {
    /// This machine's `NetBIOS` name; accounts from here are shown without a domain.
    computer: String,
    /// SID bytes to display name (or `None` when the SID could not be named).
    names: HashMap<[u8; MAX_SID], Option<String>>,
    /// UTF-16 scratch for paths and account names.
    wide: Vec<u16>,
    /// 8-byte-aligned scratch for structures the kernel fills in.
    nt: AlignedBuf,
    /// NT device prefixes and their drive letters: `\Device\HarddiskVolume3` is `C`.
    drives: Vec<(String, char)>,
    drives_at: Instant,
    /// Version strings by image path.
    verinfo: VerInfoCache,
}

impl DetailProbe {
    pub fn new() -> Self {
        let mut p = Self {
            computer: computer_name(),
            names: HashMap::new(),
            wide: Vec::with_capacity(1024),
            nt: AlignedBuf::default(),
            drives: Vec::new(),
            drives_at: Instant::now(),
            verinfo: VerInfoCache::new(),
        };
        p.refresh_drives();
        p
    }

    /// Everything that can be learned about `pid` from here.
    pub fn query(&mut self, pid: u32) -> Details {
        let mut d = Details::default();
        // The idle pseudo-process and the kernel: nothing handle-based applies, and
        // the owner is not in doubt.
        if pid == 0 || pid == 4 {
            d.user = Some("SYSTEM".to_owned());
            d.integrity = Integrity::System;
            return d;
        }
        // SAFETY: plain call; the handle is closed below.
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(h) => {
                d.image_path = self.image_path(h);
                d.command_line = self.command_line(h);
                d.architecture = architecture(h);
                self.token_details(h, &mut d);
                // SAFETY: `h` came from OpenProcess and is closed exactly once.
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
            Err(_) => d.image_path = self.image_path_by_pid(pid),
        }
        if let Some(path) = &d.image_path {
            let v = self.verinfo.get(path);
            d.description.clone_from(&v.description);
            d.company.clone_from(&v.company);
        }
        d
    }

    fn image_path(&mut self, h: HANDLE) -> Option<String> {
        if self.wide.len() < 1024 {
            self.wide.resize(1024, 0);
        }
        loop {
            let mut len = self.wide.len() as u32;
            // SAFETY: the buffer is `len` units long; `len` is a valid in-out pointer.
            let r = unsafe {
                QueryFullProcessImageNameW(
                    h,
                    PROCESS_NAME_WIN32,
                    PWSTR(self.wide.as_mut_ptr()),
                    &raw mut len,
                )
            };
            match r {
                Ok(()) => return non_empty(&self.wide[..len as usize]),
                Err(e)
                    if e.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
                        && self.wide.len() < MAX_WIDE_PATH =>
                {
                    let n = (self.wide.len() * 2).min(MAX_WIDE_PATH);
                    self.wide.resize(n, 0);
                }
                Err(_) => return None,
            }
        }
    }

    /// `ProcessCommandLineInformation` (Windows 8.1+) returns the command line as a
    /// `UNICODE_STRING` whose characters follow it in the same buffer. This is the
    /// supported way; reading the PEB across a bitness boundary is not.
    fn command_line(&mut self, h: HANDLE) -> Option<String> {
        if self.nt.len_bytes() < 2048 {
            self.nt.resize_bytes(2048);
        }
        loop {
            let mut needed = 0u32;
            // SAFETY: buffer pointer and length agree; `needed` is a valid out-pointer.
            let status = unsafe {
                NtQueryInformationProcess(
                    h,
                    ProcessCommandLineInformation,
                    self.nt.as_mut_ptr().cast(),
                    self.nt.len_bytes() as u32,
                    &raw mut needed,
                )
            };
            if status == STATUS_SUCCESS {
                break;
            }
            let too_small = status == STATUS_INFO_LENGTH_MISMATCH
                || status == STATUS_BUFFER_TOO_SMALL
                || status == STATUS_BUFFER_OVERFLOW;
            let needed = needed as usize;
            if too_small && needed > self.nt.len_bytes() && needed <= MAX_COMMAND_LINE_BYTES {
                self.nt.resize_bytes(needed);
                continue;
            }
            return None;
        }
        // SAFETY: on success the kernel wrote a UNICODE_STRING at the start of the
        // buffer, with `Buffer` pointing at characters later in the same buffer.
        let u = unsafe { &*self.nt.as_ptr().cast::<UNICODE_STRING>() };
        let s = unicode_to_string(u);
        (!s.is_empty()).then_some(s)
    }

    fn token_details(&mut self, h: HANDLE, d: &mut Details) {
        let mut tok = HANDLE::default();
        // SAFETY: `tok` is a valid out-pointer; the token is closed below.
        if unsafe { OpenProcessToken(h, TOKEN_QUERY, &raw mut tok) }.is_err() {
            return;
        }
        if let Some(sid) = self.token_sid(tok, TokenUser) {
            d.user = self.name_for(&sid);
        }
        if let Some(sid) = self.token_sid(tok, TokenIntegrityLevel) {
            d.integrity = integrity_of(&sid);
        }
        // SAFETY: closed exactly once.
        unsafe {
            let _ = CloseHandle(tok);
        }
    }

    /// The SID at the head of a `TOKEN_USER` or `TOKEN_MANDATORY_LABEL`; both begin
    /// with a `SID_AND_ATTRIBUTES`.
    fn token_sid(&mut self, tok: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Option<Sid> {
        if self.nt.len_bytes() < 256 {
            self.nt.resize_bytes(256);
        }
        let mut needed = 0u32;
        // SAFETY: buffer and length agree; `needed` is a valid out-pointer.
        let first = unsafe {
            GetTokenInformation(
                tok,
                class,
                Some(self.nt.as_mut_ptr().cast()),
                self.nt.len_bytes() as u32,
                &raw mut needed,
            )
        };
        if first.is_err() {
            let needed = needed as usize;
            if needed <= self.nt.len_bytes() || needed > 4096 {
                return None;
            }
            self.nt.resize_bytes(needed);
            let mut again = 0u32;
            // SAFETY: as above, with the size the first call asked for.
            unsafe {
                GetTokenInformation(
                    tok,
                    class,
                    Some(self.nt.as_mut_ptr().cast()),
                    self.nt.len_bytes() as u32,
                    &raw mut again,
                )
            }
            .ok()?;
        }
        // SAFETY: the call succeeded and both structures start with SID_AND_ATTRIBUTES.
        let sa = unsafe { &*self.nt.as_ptr().cast::<SID_AND_ATTRIBUTES>() };
        if sa.Sid.0.is_null() {
            return None;
        }
        // SAFETY: `Sid` points into the buffer we still hold; the header is readable.
        let len = unsafe { GetLengthSid(sa.Sid) } as usize;
        if len == 0 || len > MAX_SID {
            return None;
        }
        let mut sid = Sid {
            bytes: [0; MAX_SID],
        };
        // SAFETY: GetLengthSid says `len` bytes are readable at `Sid`; the copy fits.
        unsafe {
            std::ptr::copy_nonoverlapping(sa.Sid.0.cast::<u8>(), sid.bytes.as_mut_ptr(), len);
        }
        Some(sid)
    }

    fn name_for(&mut self, sid: &Sid) -> Option<String> {
        if let Some(n) = self.names.get(&sid.bytes) {
            return n.clone();
        }
        let name = self.lookup_account(sid).or_else(|| sid_string(sid));
        self.names.insert(sid.bytes, name.clone());
        name
    }

    fn lookup_account(&mut self, sid: &Sid) -> Option<String> {
        let mut name_len = 0u32;
        let mut domain_len = 0u32;
        let mut kind = SID_NAME_USE(0);
        // SAFETY: a size query; null buffers with zero lengths are the documented form.
        let _ = unsafe {
            LookupAccountSidW(
                PCWSTR::null(),
                sid.as_psid(),
                None,
                &raw mut name_len,
                None,
                &raw mut domain_len,
                &raw mut kind,
            )
        };
        if name_len == 0 {
            return None;
        }
        let (n, d) = (name_len as usize, domain_len as usize);
        self.wide.clear();
        self.wide.resize(n + d, 0);
        let (name_buf, domain_buf) = self.wide.split_at_mut(n);
        // SAFETY: the two buffers are the sizes the first call asked for.
        unsafe {
            LookupAccountSidW(
                PCWSTR::null(),
                sid.as_psid(),
                Some(PWSTR(name_buf.as_mut_ptr())),
                &raw mut name_len,
                Some(PWSTR(domain_buf.as_mut_ptr())),
                &raw mut domain_len,
                &raw mut kind,
            )
        }
        .ok()?;
        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        let domain = String::from_utf16_lossy(&domain_buf[..domain_len as usize]);
        Some(display_name(&self.computer, &domain, &name))
    }

    /// Image path without a handle, for processes that refuse to be opened.
    fn image_path_by_pid(&mut self, pid: u32) -> Option<String> {
        if self.wide.len() < 512 {
            self.wide.resize(512, 0);
        }
        loop {
            let capacity = (self.wide.len() * 2).min(usize::from(u16::MAX)) as u16;
            let mut info = SystemProcessIdInformation {
                ProcessId: HANDLE(pid as usize as *mut std::ffi::c_void),
                ImageName: UNICODE_STRING {
                    Length: 0,
                    MaximumLength: capacity,
                    Buffer: PWSTR(self.wide.as_mut_ptr()),
                },
            };
            let mut returned = 0u32;
            // SAFETY: `info` is fully initialized and its buffer is `capacity` bytes.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_ID_INFORMATION,
                    (&raw mut info).cast(),
                    size_of::<SystemProcessIdInformation>() as u32,
                    &raw mut returned,
                )
            };
            if status == STATUS_SUCCESS {
                let nt_path = unicode_to_string(&info.ImageName);
                return (!nt_path.is_empty()).then(|| self.dos_path(nt_path));
            }
            let want = usize::from(info.ImageName.MaximumLength);
            if status == STATUS_INFO_LENGTH_MISMATCH && want > usize::from(capacity) {
                self.wide.resize(want / 2 + 1, 0);
                continue;
            }
            return None;
        }
    }

    /// `\Device\HarddiskVolume3\Windows\...` to `C:\Windows\...`. A path on a volume
    /// with no drive letter stays as it is; that is still a real path.
    fn dos_path(&mut self, nt: String) -> String {
        if let Some(p) = self.map_drive(&nt) {
            return p;
        }
        if self.drives_at.elapsed() > DRIVE_REFRESH {
            self.refresh_drives();
            if let Some(p) = self.map_drive(&nt) {
                return p;
            }
        }
        nt
    }

    fn map_drive(&self, nt: &str) -> Option<String> {
        map_drive(&self.drives, nt)
    }

    fn refresh_drives(&mut self) {
        self.drives.clear();
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        let mut target = [0u16; 512];
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = char::from(b'A' + i);
            let device = [u16::from(b'A' + i), u16::from(b':'), 0];
            // SAFETY: `device` is NUL-terminated; the target buffer is sized.
            let n = unsafe { QueryDosDeviceW(PCWSTR(device.as_ptr()), Some(&mut target)) };
            if n == 0 {
                continue;
            }
            // A NUL-separated list; the first entry is the current mapping.
            let end = target.iter().position(|&c| c == 0).unwrap_or(0);
            if end > 0 {
                self.drives
                    .push((String::from_utf16_lossy(&target[..end]), letter));
            }
        }
        self.drives_at = Instant::now();
    }
}

fn map_drive(drives: &[(String, char)], nt: &str) -> Option<String> {
    drives.iter().find_map(|(prefix, letter)| {
        let rest = nt.strip_prefix(prefix.as_str())?;
        rest.starts_with('\\').then(|| format!("{letter}:{rest}"))
    })
}

/// How Task Manager shows an account: `SYSTEM`, `LOCAL SERVICE`, a bare name for a
/// local account, and `DOMAIN\name` for everything else (domain users, `NT SERVICE`
/// virtual accounts, `Window Manager\DWM-1`).
fn display_name(computer: &str, domain: &str, name: &str) -> String {
    if domain.is_empty()
        || domain.eq_ignore_ascii_case("NT AUTHORITY")
        || domain.eq_ignore_ascii_case(computer)
    {
        name.to_owned()
    } else {
        format!("{domain}\\{name}")
    }
}

fn integrity_of(sid: &Sid) -> Integrity {
    let psid = sid.as_psid();
    // SAFETY: the SID is a valid copy; the returned pointers address its header.
    let rid = unsafe {
        let count = *GetSidSubAuthorityCount(psid);
        if count == 0 {
            return Integrity::Unknown;
        }
        *GetSidSubAuthority(psid, u32::from(count) - 1)
    };
    match rid {
        r if r < MANDATORY_MEDIUM_RID => Integrity::Low,
        r if r < MANDATORY_HIGH_RID => Integrity::Medium,
        r if r < MANDATORY_SYSTEM_RID => Integrity::High,
        _ => Integrity::System,
    }
}

/// `S-1-5-21-...` for a SID that no longer resolves to an account.
fn sid_string(sid: &Sid) -> Option<String> {
    let mut s = PWSTR::null();
    // SAFETY: `s` receives a LocalAlloc'd string, freed here after copying.
    unsafe {
        ConvertSidToStringSidW(sid.as_psid(), &raw mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
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

fn non_empty(units: &[u16]) -> Option<String> {
    (!units.is_empty()).then(|| String::from_utf16_lossy(units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_display_follows_task_manager() {
        assert_eq!(display_name("BOX", "NT AUTHORITY", "SYSTEM"), "SYSTEM");
        assert_eq!(display_name("BOX", "box", "cameron"), "cameron");
        assert_eq!(display_name("BOX", "CORP", "cameron"), "CORP\\cameron");
        assert_eq!(
            display_name("BOX", "NT SERVICE", "TrustedInstaller"),
            "NT SERVICE\\TrustedInstaller"
        );
        assert_eq!(display_name("BOX", "", "orphan"), "orphan");
    }

    #[test]
    fn nt_paths_map_to_drive_letters_by_whole_component() {
        let drives = vec![
            ("\\Device\\HarddiskVolume3".to_owned(), 'C'),
            ("\\Device\\HarddiskVolume30".to_owned(), 'D'),
        ];
        assert_eq!(
            map_drive(&drives, "\\Device\\HarddiskVolume3\\Windows\\x.exe").as_deref(),
            Some("C:\\Windows\\x.exe")
        );
        assert_eq!(
            map_drive(&drives, "\\Device\\HarddiskVolume30\\y.exe").as_deref(),
            Some("D:\\y.exe")
        );
        assert_eq!(map_drive(&drives, "\\Device\\Mup\\server\\z.exe"), None);
    }

    /// `S-1-16-<rid>` as raw bytes: revision, sub-authority count, the six-byte
    /// mandatory-label authority, one little-endian sub-authority.
    fn label(rid: u32) -> Sid {
        let mut bytes = [0u8; MAX_SID];
        bytes[0] = 1;
        bytes[1] = 1;
        bytes[7] = 16;
        bytes[8..12].copy_from_slice(&rid.to_le_bytes());
        Sid { bytes }
    }

    #[test]
    fn integrity_comes_from_the_label_rid() {
        assert_eq!(integrity_of(&label(0x1000)), Integrity::Low);
        assert_eq!(integrity_of(&label(0x2000)), Integrity::Medium);
        assert_eq!(integrity_of(&label(0x2100)), Integrity::Medium);
        assert_eq!(integrity_of(&label(0x3000)), Integrity::High);
        assert_eq!(integrity_of(&label(0x4000)), Integrity::System);
    }

    #[test]
    fn own_process_has_details() {
        let mut p = DetailProbe::new();
        let d = p.query(std::process::id());
        let path = d.image_path.expect("own image path");
        assert!(path.to_ascii_lowercase().ends_with(".exe"), "{path}");
        assert!(d.command_line.is_some());
        assert!(d.user.is_some());
        assert_ne!(d.integrity, Integrity::Unknown);
        assert_ne!(d.architecture, Architecture::Unknown);
        // Second query of the same user hits the cache.
        assert_eq!(p.names.len(), 1);
    }

    #[test]
    fn machines_map_to_architectures() {
        assert_eq!(architecture_of(IMAGE_FILE_MACHINE_AMD64), Architecture::X64);
        assert_eq!(architecture_of(IMAGE_FILE_MACHINE_I386), Architecture::X86);
        assert_eq!(
            architecture_of(IMAGE_FILE_MACHINE_ARM64),
            Architecture::Arm64
        );
        assert_eq!(architecture_of(IMAGE_FILE_MACHINE_ARMNT), Architecture::Arm);
        assert_eq!(
            architecture_of(IMAGE_FILE_MACHINE_UNKNOWN),
            Architecture::Unknown
        );
    }

    /// A test binary has no version resource, so the check runs on Explorer,
    /// found by name in the kernel's process list. A session with no Explorer
    /// (a service account) is skipped.
    #[test]
    fn a_known_image_gets_its_description_and_company() {
        let Some(pid) = super::super::find_pid_by_name("explorer.exe") else {
            eprintln!("no explorer.exe running; skipping");
            return;
        };
        let mut p = DetailProbe::new();
        let d = p.query(pid);
        assert_eq!(d.description.as_deref(), Some("Windows Explorer"), "{d:?}");
        assert!(
            d.company
                .as_deref()
                .is_some_and(|c| c.contains("Microsoft")),
            "{d:?}"
        );
        // The same image again is served from the cache.
        assert_eq!(p.verinfo.len(), 1);
        p.query(pid);
        assert_eq!(p.verinfo.len(), 1);
    }

    #[test]
    fn own_process_reads_its_efficiency_mode() {
        // Nothing has put the test runner in efficiency mode.
        assert_eq!(efficiency_mode_of(std::process::id()), Some(false));
        // The idle process and the kernel are never asked.
        assert_eq!(efficiency_mode_of(0), None);
        assert_eq!(efficiency_mode_of(4), None);
    }
}
