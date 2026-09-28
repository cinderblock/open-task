//! A physical disk's facts: model, solid state or not, capacity.
//!
//! The disk is opened with no access rights at all (`\\.\PhysicalDriveN`, desired
//! access 0), which Windows allows without elevation for exactly this: storage
//! property queries and geometry. Reading or writing would need administrator.
//! Everything here runs once per disk, when it is first seen.

use std::mem::size_of;

use ot_model::device::DiskInfo;
use ot_model::Bytes;
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, StorageDeviceSeekPenaltyProperty,
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID,
    STORAGE_PROPERTY_QUERY,
};
use windows::Win32::System::IO::DeviceIoControl;

use super::AlignedBuf;

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

/// Facts for disk `number`, whose volumes are `letters` (`C: D:`, or empty).
pub(super) fn disk_info(number: u32, letters: &str) -> DiskInfo {
    let mut info = DiskInfo {
        number,
        name: display_name(number, letters),
        ..DiskInfo::default()
    };
    let path = HSTRING::from(format!(r"\\.\PhysicalDrive{number}"));
    // SAFETY: a zero-access open of a device path; the handle is owned below.
    let Ok(h) = (unsafe {
        CreateFileW(
            &path,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }) else {
        return info;
    };
    let dev = Device(h);
    let mut buf = AlignedBuf::default();
    buf.resize_bytes(1024);

    if let Some(len) = query(&dev, StorageDeviceProperty, &mut buf) {
        // SAFETY: the query succeeded and wrote at least a descriptor header.
        let d = unsafe { &*buf.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>() };
        info.removable = d.RemovableMedia;
        let bytes = bytes_of(&buf, len);
        let vendor = c_string(bytes, d.VendorIdOffset);
        let product = c_string(bytes, d.ProductIdOffset);
        info.model = model(vendor.as_deref(), product.as_deref());
    }
    if query(&dev, StorageDeviceSeekPenaltyProperty, &mut buf).is_some() {
        // SAFETY: the query succeeded and wrote the descriptor.
        let d = unsafe { &*buf.as_ptr().cast::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() };
        info.ssd = Some(!d.IncursSeekPenalty);
    }
    let mut returned = 0u32;
    // SAFETY: the output buffer is 1024 bytes, larger than DISK_GEOMETRY_EX.
    let geometry = unsafe {
        DeviceIoControl(
            dev.0,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some(buf.as_mut_ptr().cast()),
            buf.len_bytes() as u32,
            Some(&raw mut returned),
            None,
        )
    };
    if geometry.is_ok() && returned as usize >= size_of::<DISK_GEOMETRY_EX>() - 8 {
        // SAFETY: the call succeeded and wrote the fixed part of the structure.
        let g = unsafe { &*buf.as_ptr().cast::<DISK_GEOMETRY_EX>() };
        info.capacity = u64::try_from(g.DiskSize).ok().filter(|&b| b > 0).map(Bytes);
    }
    info
}

/// `Disk 0 (C: D:)`, or `Disk 2` for a disk with no lettered volume.
pub(super) fn display_name(number: u32, letters: &str) -> String {
    let letters = letters.trim();
    if letters.is_empty() {
        format!("Disk {number}")
    } else {
        format!("Disk {number} ({letters})")
    }
}

/// One standard storage property query into `buf`. Returns the bytes written.
fn query(dev: &Device, property: STORAGE_PROPERTY_ID, buf: &mut AlignedBuf) -> Option<usize> {
    let q = STORAGE_PROPERTY_QUERY {
        PropertyId: property,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut returned = 0u32;
    // SAFETY: the query struct and the output buffer outlive the call; sizes match.
    unsafe {
        DeviceIoControl(
            dev.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const q).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr().cast()),
            buf.len_bytes() as u32,
            Some(&raw mut returned),
            None,
        )
    }
    .ok()?;
    (returned > 0).then_some(returned as usize)
}

fn bytes_of(buf: &AlignedBuf, len: usize) -> &[u8] {
    // SAFETY: `len` bytes were written by the kernel into the buffer, and `len`
    // never exceeds its size.
    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len.min(buf.len_bytes())) }
}

/// A NUL-terminated ASCII string at `offset` in the descriptor, trimmed. Offset 0
/// means "not present".
fn c_string(bytes: &[u8], offset: u32) -> Option<String> {
    let start = offset as usize;
    if start == 0 || start >= bytes.len() {
        return None;
    }
    let end = bytes[start..]
        .iter()
        .position(|&b| b == 0)
        .map_or(bytes.len(), |n| start + n);
    let s = String::from_utf8_lossy(&bytes[start..end])
        .trim()
        .to_owned();
    (!s.is_empty()).then_some(s)
}

/// Vendor and product as one model name. `NVMe` drives often report the vendor as
/// `NVMe` and put the real maker in the product string; SATA drives often leave the
/// vendor empty. Drop the vendor when it is empty, generic, or already in the
/// product.
fn model(vendor: Option<&str>, product: Option<&str>) -> Option<String> {
    let product = product?;
    match vendor {
        Some(v)
            if !v.eq_ignore_ascii_case("NVMe")
                && !v.eq_ignore_ascii_case("ATA")
                && !product
                    .to_ascii_lowercase()
                    .contains(&v.to_ascii_lowercase()) =>
        {
            Some(format!("{v} {product}"))
        }
        _ => Some(product.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_models_read_well() {
        assert_eq!(display_name(0, "C:"), "Disk 0 (C:)");
        assert_eq!(display_name(1, " D: E: "), "Disk 1 (D: E:)");
        assert_eq!(display_name(2, ""), "Disk 2");
        assert_eq!(
            model(Some("NVMe"), Some("Samsung SSD 970 EVO Plus 1TB")).as_deref(),
            Some("Samsung SSD 970 EVO Plus 1TB")
        );
        assert_eq!(
            model(Some("WDC"), Some("WD10EZEX-00BN5A0")).as_deref(),
            Some("WDC WD10EZEX-00BN5A0")
        );
        assert_eq!(
            model(Some("Samsung"), Some("Samsung SSD")).as_deref(),
            Some("Samsung SSD")
        );
        assert_eq!(
            model(None, Some("KINGSTON SA400")).as_deref(),
            Some("KINGSTON SA400")
        );
        assert_eq!(model(Some("x"), None), None);
    }

    #[test]
    fn c_strings_stop_at_nul_and_respect_bounds() {
        let b = b"\0\0\0\0ABC  \0DEF";
        assert_eq!(c_string(b, 4).as_deref(), Some("ABC"));
        assert_eq!(c_string(b, 10).as_deref(), Some("DEF"));
        assert_eq!(c_string(b, 0), None);
        assert_eq!(c_string(b, 99), None);
    }

    #[test]
    fn the_first_disk_describes_itself() {
        let d = disk_info(0, "C:");
        assert_eq!(d.name, "Disk 0 (C:)");
        assert!(d.model.is_some(), "{d:?}");
        assert!(d.capacity.is_some_and(|c| c.get() > 1 << 30), "{d:?}");
    }
}
