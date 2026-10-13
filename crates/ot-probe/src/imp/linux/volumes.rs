//! Mounted volumes, from `/proc/self/mountinfo` and `statvfs`.
//!
//! Only block-backed file systems are volumes here: pseudo file systems (`proc`,
//! `tmpfs`, `cgroup2`) and loop-mounted images (snaps) are not. A device mounted
//! in several places (bind mounts, btrfs subvolumes) is listed once, at its
//! shortest mount path. The list is read every few seconds, not every pass.

use std::collections::HashMap;
use std::ffi::CString;
use std::time::{Duration, Instant};

use ot_model::device::VolumeSample;
use ot_model::units::Bytes;

use super::disks::{self, DiskProbe};

const REFRESH: Duration = Duration::from_secs(5);

/// One mount, as `/proc/self/mountinfo` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    major_minor: String,
    path: String,
    fs_type: String,
    source: String,
}

/// Undo the octal escapes the kernel writes for spaces and the like (`\040`).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            // `id parent major:minor root mount-point options [optional…] - type source super-options`
            let (left, right) = line.split_once(" - ")?;
            let mut l = left.split(' ');
            let major_minor = l.nth(2)?.to_owned();
            let path = unescape(l.nth(1)?);
            let mut r = right.split(' ');
            let fs_type = r.next()?.to_owned();
            let source = unescape(r.next()?);
            Some(Mount {
                major_minor,
                path,
                fs_type,
                source,
            })
        })
        .collect()
}

/// Whether a mount is a volume: a block device that is not a loop image.
fn is_volume(m: &Mount) -> bool {
    m.source.starts_with("/dev/") && !m.source.starts_with("/dev/loop")
}

/// Free and total bytes of the file system at `path`, as an unprivileged caller
/// sees them (`f_bavail`: root's reserve is not free to anyone else).
fn space(path: &str) -> Option<(u64, u64)> {
    let c = CString::new(path).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter; `c` is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &raw mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize;
    Some((st.f_blocks * frsize, st.f_bavail * frsize))
}

/// File system labels by device name, from `/dev/disk/by-label`.
fn labels() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/dev/disk/by-label") else {
        return out;
    };
    for e in dir.flatten() {
        let label = unescape_udev(&e.file_name().to_string_lossy());
        if let Ok(target) = std::fs::read_link(e.path()) {
            if let Some(dev) = target.file_name() {
                out.insert(dev.to_string_lossy().into_owned(), label);
            }
        }
    }
    out
}

/// udev writes `\x20` for a space in a label.
fn unescape_udev(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\\x") {
        out.push_str(&rest[..i]);
        if let Some(b) = rest
            .get(i + 2..i + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(char::from(b));
            rest = &rest[i + 4..];
        } else {
            out.push_str("\\x");
            rest = &rest[i + 2..];
        }
    }
    out.push_str(rest);
    out
}

/// Swap files, from `/proc/swaps` (swap partitions are not on a volume).
fn swap_files() -> Vec<String> {
    std::fs::read_to_string("/proc/swaps")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_ascii_whitespace();
            let path = f.next()?;
            (f.next()? == "file").then(|| unescape(path))
        })
        .collect()
}

#[derive(Debug, Default)]
pub(super) struct VolumeProbe {
    volumes: Vec<VolumeSample>,
    next: Option<Instant>,
}

impl VolumeProbe {
    pub(super) fn sample(&mut self, out: &mut Vec<VolumeSample>, disks: &mut DiskProbe) {
        let now = Instant::now();
        if self.next.is_none_or(|t| now >= t) {
            self.next = Some(now + REFRESH);
            self.refresh(disks);
        }
        out.clone_from(&self.volumes);
    }

    fn refresh(&mut self, disks: &mut DiskProbe) {
        let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        let mut by_device: HashMap<String, Mount> = HashMap::new();
        for m in parse_mountinfo(&text).into_iter().filter(is_volume) {
            match by_device.get(&m.major_minor) {
                Some(seen) if seen.path.len() <= m.path.len() => {}
                _ => {
                    by_device.insert(m.major_minor.clone(), m);
                }
            }
        }
        let labels = labels();
        let swaps = swap_files();
        let mut disk_mounts: HashMap<String, Vec<String>> = HashMap::new();
        self.volumes.clear();
        for m in by_device.into_values() {
            let Some((total, free)) = space(&m.path) else {
                continue;
            };
            let dev = disks::device_name(&m.major_minor);
            let under = dev.as_deref().map(disks::disks_under).unwrap_or_default();
            for d in &under {
                disk_mounts
                    .entry(d.clone())
                    .or_default()
                    .push(m.path.clone());
            }
            let on_this = |path: &str| {
                path.strip_prefix(m.path.as_str())
                    .is_some_and(|rest| m.path == "/" || rest.starts_with('/'))
            };
            self.volumes.push(VolumeSample {
                label: dev.as_ref().and_then(|d| labels.get(d).cloned()),
                filesystem: Some(m.fs_type.clone()),
                total: Bytes(total),
                free: Bytes(free),
                disk: under.first().map(|d| disks.number_of(d)),
                system: m.path == "/",
                page_file: swaps.iter().any(|s| on_this(s)),
                mount: m.path,
            });
        }
        for v in disk_mounts.values_mut() {
            v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        }
        disks.set_mounts(disk_mounts);
        // `/` first, then by path.
        self.volumes
            .sort_by(|a, b| b.system.cmp(&a.system).then_with(|| a.mount.cmp(&b.mount)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_lines_parse() {
        let m = parse_mountinfo(
            "29 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
             40 29 0:35 / /mnt/my\\040disk rw - vfat /dev/sdb1 rw\n\
             41 29 0:5 / /proc rw - proc proc rw\n",
        );
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].major_minor, "8:2");
        assert_eq!(m[0].path, "/");
        assert_eq!(m[0].fs_type, "ext4");
        assert_eq!(m[0].source, "/dev/sda2");
        assert_eq!(m[1].path, "/mnt/my disk");
        assert!(is_volume(&m[0]));
        assert!(!is_volume(&m[2]));
    }

    #[test]
    fn udev_labels_unescape() {
        assert_eq!(unescape_udev("My\\x20Disk"), "My Disk");
        assert_eq!(unescape_udev("plain"), "plain");
    }

    #[test]
    fn the_root_volume_is_listed_first() {
        let mut v = VolumeProbe::default();
        let mut disks = DiskProbe::default();
        let mut out = Vec::new();
        v.sample(&mut out, &mut disks);
        if let Some(first) = out.first() {
            assert!(first.total.get() >= first.free.get());
            if out.iter().any(|v| v.system) {
                assert!(first.system);
            }
        }
    }
}
