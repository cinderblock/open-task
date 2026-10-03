//! The machine and its operating system, for the System page. Read on demand;
//! nothing here changes while Windows runs.
//!
//! - Computer name: `GetComputerNameExW(ComputerNameDnsHostname)`.
//! - Edition, version and build: `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`.
//!   `ProductName` still says `Windows 10 Pro` on Windows 11 (Microsoft never
//!   updated the value), so builds from 22000 up have the leading `Windows 10`
//!   rewritten. `DisplayVersion` is the marketing version (`24H2`); the build is
//!   `CurrentBuild` with the `UBR` revision after a dot, as `winver` shows it;
//!   `InstallDate` is Unix seconds.
//! - Firmware, board and memory modules: the SMBIOS table, see [`super::smbios`].
//! - UEFI: `GetFirmwareType`. Secure Boot: the `UEFISecureBootEnabled` value
//!   under `HKLM\SYSTEM\CurrentControlSet\Control\SecureBoot\State`, which the
//!   kernel writes at boot; a legacy-BIOS machine has no key, and reports `None`.
//! - Page files: what the registry configures (`PagingFiles`, where `?:` means
//!   "system managed, wherever Windows chooses") rendered with the live size from
//!   `NtQuerySystemInformation(SystemPageFileInformation)`, which an unelevated
//!   caller may read. The live list is also what the volume probe uses to flag the
//!   volume holding a page file, since the registry's `?:` names no volume.
//!
//! WMI (`Win32_OperatingSystem`, `Win32_ComputerSystem`) would give the same facts
//! through COM in tens of milliseconds; all of this together took half a
//! millisecond here, most of it the SMBIOS read.

use std::mem::size_of;

use ot_model::system::SystemFacts;
use ot_model::Bytes;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS;
use windows::Win32::Foundation::UNICODE_STRING;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::{
    ComputerNameDnsHostname, FirmwareTypeUefi, GetComputerNameExW, GetFirmwareType, GetSystemInfo,
    FIRMWARE_TYPE, SYSTEM_INFO,
};

use super::smbios::Smbios;
use super::{hardware, query_growing, unicode_to_string, AlignedBuf};

const CURRENT_VERSION: PCWSTR = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
const SECURE_BOOT: PCWSTR = w!(r"SYSTEM\CurrentControlSet\Control\SecureBoot\State");
const MEMORY_MANAGEMENT: PCWSTR =
    w!(r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management");

/// The first Windows 11 build.
const FIRST_WINDOWS_11_BUILD: u32 = 22000;

/// `SystemPageFileInformation`, absent from the SDK metadata. The class number
/// and structure are from `phnt`'s `ntexapi.h`; sizes are in pages.
const SYSTEM_PAGE_FILE_INFORMATION: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(18);

/// One page file, as the kernel reports it. `PageFileName` is an NT path
/// (`\??\C:\pagefile.sys`) whose characters follow the structure in the buffer.
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy)]
struct SystemPageFileInformation {
    NextEntryOffset: u32,
    TotalSize: u32,
    TotalInUse: u32,
    PeakUsage: u32,
    PageFileName: UNICODE_STRING,
}

/// A page file's path and, where the kernel reported it, its current size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageFile {
    /// `C:\pagefile.sys`.
    pub path: String,
    pub size: Option<Bytes>,
}

/// Everything this module can learn. Each fact that cannot be read is left empty.
pub(super) fn facts() -> SystemFacts {
    let mut facts = SystemFacts {
        computer_name: computer_name(),
        installed_memory: hardware::installed_memory(),
        uefi: uefi(),
        secure_boot: reg_dword(SECURE_BOOT, w!("UEFISecureBootEnabled")).map(|v| v != 0),
        page_files: page_files().iter().map(render_page_file).collect(),
        ..SystemFacts::default()
    };
    operating_system(&mut facts);
    if let Some(smbios) = Smbios::read() {
        if let Some(b) = smbios.bios() {
            facts.bios_vendor = b.vendor;
            facts.bios_version = b.version;
            facts.bios_date = b.release_date;
        }
        if let Some(s) = smbios.system() {
            facts.system_manufacturer = s.manufacturer;
            facts.system_model = s.product;
        }
        if let Some(b) = smbios.baseboard() {
            facts.board_manufacturer = b.manufacturer;
            facts.board_product = b.product;
        }
        facts.memory_devices = smbios.memory_devices();
    }
    facts
}

fn computer_name() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer and its length in characters agree.
    unsafe {
        GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(PWSTR(buf.as_mut_ptr())),
            &raw mut len,
        )
    }
    .ok()?;
    let name = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
    (!name.is_empty()).then_some(name)
}

fn uefi() -> Option<bool> {
    let mut kind = FIRMWARE_TYPE::default();
    // SAFETY: `kind` is a valid out-pointer.
    unsafe { GetFirmwareType(&raw mut kind) }.ok()?;
    Some(kind == FirmwareTypeUefi)
}

/// Edition, version, build and install date from the registry.
fn operating_system(facts: &mut SystemFacts) {
    let build = reg_sz(CURRENT_VERSION, w!("CurrentBuildNumber"))
        .or_else(|| reg_sz(CURRENT_VERSION, w!("CurrentBuild")));
    let build_number: Option<u32> = build.as_deref().and_then(|b| b.parse().ok());
    facts.os_name =
        reg_sz(CURRENT_VERSION, w!("ProductName")).map(|name| os_name(&name, build_number));
    facts.os_version = reg_sz(CURRENT_VERSION, w!("DisplayVersion"));
    facts.os_build = build.map(|b| match reg_dword(CURRENT_VERSION, w!("UBR")) {
        Some(ubr) => format!("{b}.{ubr}"),
        None => b,
    });
    facts.os_installed_unix_ms =
        reg_dword(CURRENT_VERSION, w!("InstallDate")).map(|secs| i64::from(secs) * 1000);
}

/// `ProductName` with the Windows 11 correction applied.
fn os_name(product: &str, build: Option<u32>) -> String {
    match (product.strip_prefix("Windows 10"), build) {
        (Some(rest), Some(b)) if b >= FIRST_WINDOWS_11_BUILD => format!("Windows 11{rest}"),
        _ => product.to_owned(),
    }
}

/// The page files in use: the kernel's live list with sizes, or when that cannot
/// be read, the registry's configured list without. Paths are `C:\pagefile.sys`.
pub(super) fn page_files() -> Vec<PageFile> {
    let configured = configured_page_files();
    let live = live_page_files();
    if live.is_empty() {
        return configured
            .into_iter()
            .map(|path| PageFile { path, size: None })
            .collect();
    }
    live
}

/// `PagingFiles` entries, reduced to their paths: `C:\pagefile.sys 0 0` is the
/// path and the initial and maximum sizes in MB; `?:\pagefile.sys` is a
/// system-managed file on whichever volume Windows picks.
fn configured_page_files() -> Vec<String> {
    reg_multi_sz(MEMORY_MANAGEMENT, w!("PagingFiles"))
        .iter()
        .filter_map(|entry| {
            let mut words: Vec<&str> = entry.split(' ').filter(|w| !w.is_empty()).collect();
            while words.len() > 1 && words.last().is_some_and(|w| w.parse::<u64>().is_ok()) {
                words.pop();
            }
            let path = words.join(" ");
            (!path.is_empty()).then_some(path)
        })
        .collect()
}

/// The kernel's page file list, with current sizes.
fn live_page_files() -> Vec<PageFile> {
    let mut out = Vec::new();
    let mut buf = AlignedBuf::default();
    buf.resize_bytes(4096);
    let Ok(len) = query_growing(
        SYSTEM_PAGE_FILE_INFORMATION,
        &mut buf,
        "SystemPageFileInformation",
    ) else {
        return out;
    };
    // A machine with no page file at all answers success with no bytes.
    if len < size_of::<SystemPageFileInformation>() {
        return out;
    }
    let mut si = SYSTEM_INFO::default();
    // SAFETY: `si` is a valid, writable SYSTEM_INFO.
    unsafe { GetSystemInfo(&raw mut si) };
    let page = u64::from(si.dwPageSize);

    let base = buf.as_ptr();
    let mut offset = 0usize;
    while offset + size_of::<SystemPageFileInformation>() <= len {
        // SAFETY: within the bytes the kernel wrote; entries are 8-byte aligned.
        let p = unsafe { &*base.byte_add(offset).cast::<SystemPageFileInformation>() };
        let name = unicode_to_string(&p.PageFileName);
        let path = name.strip_prefix(r"\??\").unwrap_or(&name).to_owned();
        out.push(PageFile {
            path,
            size: Some(Bytes(u64::from(p.TotalSize) * page)),
        });
        if p.NextEntryOffset == 0 {
            break;
        }
        offset += p.NextEntryOffset as usize;
    }
    out
}

/// `C:\pagefile.sys (8.0 GB)`, or the path alone without a size.
fn render_page_file(f: &PageFile) -> String {
    match f.size {
        Some(size) => format!("{} ({})", f.path, size_text(size)),
        None => f.path.clone(),
    }
}

/// `8.0 GB`, `512 MB`: binary units, as Windows labels page file sizes.
fn size_text(b: Bytes) -> String {
    let b = b.get() as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.0} MB", b / (1024.0 * 1024.0))
    }
}

/// A `REG_SZ` value under `HKLM`, trimmed; `None` when missing or empty.
fn reg_sz(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte size agree; the strings are static.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// A `REG_DWORD` value under `HKLM`.
fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the output is a u32 and its size says so; the strings are static.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&raw mut size),
        )
    };
    status.is_ok().then_some(data)
}

/// A `REG_MULTI_SZ` value under `HKLM`, as its non-empty strings.
fn reg_multi_sz(key: PCWSTR, value: PCWSTR) -> Vec<String> {
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte size agree; the strings are static.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            value,
            RRF_RT_REG_MULTI_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return Vec::new();
    }
    let units = (size as usize / 2).min(buf.len());
    buf[..units]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn windows_11_is_named_from_its_build() {
        assert_eq!(os_name("Windows 10 Pro", Some(26100)), "Windows 11 Pro");
        assert_eq!(os_name("Windows 10 Pro", Some(19045)), "Windows 10 Pro");
        assert_eq!(os_name("Windows 10 Pro", None), "Windows 10 Pro");
        assert_eq!(
            os_name("Windows Server 2022 Datacenter", Some(20348)),
            "Windows Server 2022 Datacenter"
        );
    }

    #[test]
    fn page_files_render_with_sizes() {
        let f = PageFile {
            path: r"C:\pagefile.sys".into(),
            size: Some(Bytes(8 << 30)),
        };
        assert_eq!(render_page_file(&f), r"C:\pagefile.sys (8.0 GB)");
        let f = PageFile {
            path: r"D:\pagefile.sys".into(),
            size: Some(Bytes(512 << 20)),
        };
        assert_eq!(render_page_file(&f), r"D:\pagefile.sys (512 MB)");
        let f = PageFile {
            path: r"?:\pagefile.sys".into(),
            size: None,
        };
        assert_eq!(render_page_file(&f), r"?:\pagefile.sys");
    }

    #[test]
    fn this_machine_describes_itself() {
        let start = Instant::now();
        let f = facts();
        let took = start.elapsed();
        println!("facts() took {took:?}: {f:#?}");
        assert!(
            f.os_name.as_deref().is_some_and(|n| n.contains("Windows")),
            "{f:?}"
        );
        let build = f.os_build.as_deref().expect("build");
        let (major, ubr) = build.split_once('.').expect("N.N");
        assert!(
            major.parse::<u32>().is_ok() && ubr.parse::<u32>().is_ok(),
            "{build}"
        );
        assert!(
            f.computer_name.as_deref().is_some_and(|n| !n.is_empty()),
            "{f:?}"
        );
        assert_eq!(f.uefi, Some(true), "{f:?}");
        assert!(f.memory_devices.iter().any(|d| d.size.is_some()), "{f:?}");
        assert!(f.installed_memory.is_some(), "{f:?}");
        assert!(
            f.os_installed_unix_ms
                .is_some_and(|t| t > 1_400_000_000_000),
            "{f:?}"
        );
        assert!(f.bios_vendor.is_some(), "{f:?}");
        assert!(
            f.page_files.iter().all(|p| p.contains(":\\")),
            "page files are paths: {:?}",
            f.page_files
        );
    }

    #[test]
    fn the_live_page_file_list_has_sizes() {
        let live = live_page_files();
        for p in &live {
            assert!(p.size.is_some_and(|s| s.get() > 0), "{live:?}");
            assert!(p.path.len() > 3 && &p.path[1..3] == ":\\", "{live:?}");
        }
        // Nothing here asserts a page file exists: a machine can run without one.
    }
}
