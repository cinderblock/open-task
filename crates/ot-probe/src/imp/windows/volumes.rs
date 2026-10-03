//! Mounted volumes: letter, label, file system, space, and which disk each is on.
//!
//! `GetLogicalDrives` is a bitmask of the letters in use; `GetDriveTypeW` keeps the
//! fixed and removable ones (network shares and optical drives are not what the
//! disk panes are about). `GetVolumeInformationW` gives the label and file system,
//! and fails for a card reader slot with nothing in it, which is how empty
//! removable slots are skipped. `GetDiskFreeSpaceExW` gives the sizes. The physical
//! disk comes from a zero-access open of `\\.\C:` and
//! `IOCTL_STORAGE_GET_DEVICE_NUMBER`, the same no-privilege trick [`super::storage`]
//! uses for disks; a volume spanning several disks (a Storage Space, a dynamic
//! volume) answers with a device type that is not a disk, and gets no number.
//!
//! The system volume is the one holding `GetWindowsDirectoryW`; the page file
//! volumes are those named in the kernel's live page file list (see
//! [`super::system::page_files`]).
//!
//! One refresh costs about half a millisecond per volume, most of it the device
//! open, so it runs every [`REFRESH_EVERY`] and passes in between copy the cached
//! list, as the network probe does with its discovery.

use std::mem::size_of;
use std::time::{Duration, Instant};

use ot_model::device::VolumeSample;
use ot_model::Bytes;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    FILE_DEVICE_DISK, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::Win32::System::IO::DeviceIoControl;

use super::system;

/// How often the volume list is rebuilt. A plugged-in drive, or a big delete,
/// shows within this long.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Closes on drop.
struct Device(HANDLE);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: opened by CreateFileW below, closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The volume list, rebuilt every [`REFRESH_EVERY`].
#[derive(Debug, Default)]
pub(super) struct VolumeProbe {
    cached: Vec<VolumeSample>,
    refreshed_at: Option<Instant>,
}

impl VolumeProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refill `out` with every listed volume, the system volume first and the rest
    /// by letter. Rebuilds the list when it is older than [`REFRESH_EVERY`].
    pub fn sample(&mut self, out: &mut Vec<VolumeSample>) {
        let now = Instant::now();
        if self
            .refreshed_at
            .is_none_or(|t| now.duration_since(t) >= REFRESH_EVERY)
        {
            self.refreshed_at = Some(now);
            self.cached = list_volumes();
        }
        out.clone_from(&self.cached);
    }
}

/// Walk the drive letters once.
fn list_volumes() -> Vec<VolumeSample> {
    let system_letter = windows_directory_letter();
    let page_file_letters: Vec<u8> = system::page_files()
        .iter()
        .filter_map(|f| drive_letter(&f.path))
        .collect();
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for bit in 0..26u32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let letter = b'A' + bit as u8;
        let Some(mut v) = volume(letter) else {
            continue;
        };
        v.system = system_letter == Some(letter);
        v.page_file = page_file_letters.contains(&letter);
        out.push(v);
    }
    out.sort_by(|a, b| b.system.cmp(&a.system).then_with(|| a.mount.cmp(&b.mount)));
    out
}

/// One lettered volume, or `None` when it is not a fixed or removable drive, or
/// a removable slot with nothing in it.
fn volume(letter: u8) -> Option<VolumeSample> {
    let root: Vec<u16> = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0].to_vec();
    let root = PCWSTR(root.as_ptr());
    // SAFETY: `root` is a NUL-terminated string that outlives the call.
    let kind = unsafe { GetDriveTypeW(root) };
    if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
        return None;
    }
    let mut label = [0u16; 261];
    let mut filesystem = [0u16; 261];
    // SAFETY: both buffers are passed with their lengths; `root` outlives the call.
    unsafe {
        GetVolumeInformationW(
            root,
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut filesystem),
        )
    }
    .ok()?;
    let mut free = 0u64;
    let mut total = 0u64;
    // SAFETY: both out-pointers are valid u64s; `root` outlives the call.
    unsafe { GetDiskFreeSpaceExW(root, Some(&raw mut free), Some(&raw mut total), None) }.ok()?;
    let mount = format!("{}:", char::from(letter));
    Some(VolumeSample {
        disk: disk_number(&mount),
        mount,
        label: wide(&label),
        filesystem: wide(&filesystem),
        total: Bytes(total),
        free: Bytes(free),
        system: false,
        page_file: false,
    })
}

/// The physical disk under `C:`, through a zero-access device open.
fn disk_number(mount: &str) -> Option<u32> {
    let path = HSTRING::from(format!(r"\\.\{mount}"));
    // SAFETY: a zero-access open of a device path; the handle is owned below.
    let h = unsafe {
        CreateFileW(
            &path,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;
    let dev = Device(h);
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let mut returned = 0u32;
    // SAFETY: the output buffer is exactly one STORAGE_DEVICE_NUMBER.
    unsafe {
        DeviceIoControl(
            dev.0,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some((&raw mut number).cast()),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&raw mut returned),
            None,
        )
    }
    .ok()?;
    (returned as usize >= size_of::<STORAGE_DEVICE_NUMBER>()
        && number.DeviceType == FILE_DEVICE_DISK.0)
        .then_some(number.DeviceNumber)
}

/// The letter of the volume holding the Windows directory.
fn windows_directory_letter() -> Option<u8> {
    let mut buf = [0u16; 261];
    // SAFETY: the buffer is passed with its length.
    let len = unsafe { GetWindowsDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    drive_letter(&String::from_utf16_lossy(&buf[..len]))
}

/// The upper-case drive letter a path like `C:\Windows` starts with.
fn drive_letter(path: &str) -> Option<u8> {
    let b = path.as_bytes();
    match (b.first(), b.get(1)) {
        (Some(l), Some(b':')) if l.is_ascii_alphabetic() => Some(l.to_ascii_uppercase()),
        _ => None,
    }
}

/// A NUL-terminated wide string, or `None` when empty.
fn wide(units: &[u16]) -> Option<String> {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    let s = String::from_utf16_lossy(&units[..end]);
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_letters_are_read_from_paths() {
        assert_eq!(drive_letter(r"c:\Windows"), Some(b'C'));
        assert_eq!(drive_letter(r"D:\pagefile.sys"), Some(b'D'));
        assert_eq!(drive_letter(r"?:\pagefile.sys"), None);
        assert_eq!(drive_letter(r"\\server\share"), None);
        assert_eq!(drive_letter(""), None);
    }

    #[test]
    fn this_machine_lists_c_first_as_the_system_volume() {
        let mut probe = VolumeProbe::new();
        let mut out = Vec::new();
        let start = Instant::now();
        probe.sample(&mut out);
        println!("first volume refresh took {:?}: {out:#?}", start.elapsed());
        let c = out.first().expect("a volume");
        assert_eq!(c.mount, "C:", "{out:?}");
        assert!(c.system, "{c:?}");
        assert!(c.total > c.free, "{c:?}");
        assert_eq!(c.filesystem.as_deref(), Some("NTFS"), "{c:?}");
        assert!(c.disk.is_some(), "{c:?}");
        assert_eq!(out.iter().filter(|v| v.system).count(), 1);
        let mut letters: Vec<&str> = out.iter().skip(1).map(|v| v.mount.as_str()).collect();
        let sorted = letters.clone();
        letters.sort_unstable();
        assert_eq!(letters, sorted, "the rest sort by letter");

        // A second pass inside the refresh interval serves the cached list.
        let first = out.clone();
        probe.sample(&mut out);
        assert_eq!(out, first);
    }
}
