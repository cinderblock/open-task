//! Physical disks, from `/proc/diskstats` and `/sys/block`.
//!
//! A block device is a physical disk when `/sys/block/<name>/device` exists: that
//! keeps disks, `NVMe` namespaces and card readers, and drops partitions, loop
//! devices, RAM disks and device-mapper volumes. A volume on LVM or LUKS is traced
//! back to the disks under it through each mapper device's `slaves`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ot_model::device::{DiskInfo, DiskSample};
use ot_model::units::{Bytes, Percent};

use super::fs;

/// The `/proc/diskstats` counters a pass differences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counters {
    reads: u64,
    sectors_read: u64,
    ms_reading: u64,
    writes: u64,
    sectors_written: u64,
    ms_writing: u64,
    /// Milliseconds the device had requests outstanding.
    ms_active: u64,
}

/// One `/proc/diskstats` line: the device name and its counters.
fn parse_line(line: &str) -> Option<(&str, Counters)> {
    let mut f = line.split_ascii_whitespace();
    let name = f.nth(2)?;
    let mut n = || f.next().and_then(|v| v.parse::<u64>().ok());
    let reads = n()?;
    let _merged = n()?;
    let sectors_read = n()?;
    let ms_reading = n()?;
    let writes = n()?;
    let _merged = n()?;
    let sectors_written = n()?;
    let ms_writing = n()?;
    let _in_flight = n()?;
    let ms_active = n()?;
    Some((
        name,
        Counters {
            reads,
            sectors_read,
            ms_reading,
            writes,
            sectors_written,
            ms_writing,
            ms_active,
        },
    ))
}

/// The disk names a block device lives on: itself for a disk, its disk for a
/// partition, and the disks under each layer for a mapper or RAID device.
pub(super) fn disks_under(dev: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_disks(dev, &mut out, 0);
    out.sort();
    out.dedup();
    out
}

fn collect_disks(dev: &str, out: &mut Vec<String>, depth: u32) {
    if depth > 8 {
        return;
    }
    if std::path::Path::new(&format!("/sys/block/{dev}/device")).exists() {
        out.push(dev.to_owned());
        return;
    }
    // A partition: `/sys/class/block/sda1` links into `…/block/sda/sda1`.
    if let Ok(target) = std::fs::read_link(format!("/sys/class/block/{dev}")) {
        let target = target.to_string_lossy();
        let mut parts = target.rsplit('/');
        if parts.next() == Some(dev) {
            if let Some(parent) = parts.next().filter(|p| *p != "block") {
                collect_disks(parent, out, depth + 1);
                return;
            }
        }
    }
    if let Ok(slaves) = std::fs::read_dir(format!("/sys/block/{dev}/slaves")) {
        for s in slaves.flatten() {
            if let Some(name) = s.file_name().to_str() {
                collect_disks(name, out, depth + 1);
            }
        }
    }
}

/// The block device behind `major:minor`, by name.
pub(super) fn device_name(major_minor: &str) -> Option<String> {
    let target = std::fs::read_link(format!("/sys/dev/block/{major_minor}")).ok()?;
    target.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// What kind of bus a disk is on, from its name and where it sits in sysfs.
fn bus_of(name: &str, sys_path: &str) -> Option<String> {
    let bus = if sys_path.contains("/usb") {
        "USB"
    } else if name.starts_with("nvme") {
        "NVMe"
    } else if name.starts_with("mmcblk") {
        "SD"
    } else if name.starts_with("vd") || sys_path.contains("/virtio") {
        "Virtio"
    } else if sys_path.contains("/ata") {
        "SATA"
    } else if sys_path.contains("VMBUS") || sys_path.contains("/vmbus") {
        "Hyper-V"
    } else if name.starts_with("sd") {
        "SCSI"
    } else {
        return None;
    };
    Some(bus.to_owned())
}

#[derive(Debug)]
struct Disk {
    info: Arc<DiskInfo>,
    prev: Counters,
}

#[derive(Debug, Default)]
pub(super) struct DiskProbe {
    disks: HashMap<String, Disk>,
    /// Every block name seen and whether it is a physical disk, so a partition is
    /// examined once.
    physical: HashMap<String, bool>,
    /// Disk numbers by name, kept while the disk is attached.
    numbers: HashMap<String, u32>,
    /// Mount points by disk name, for display names; set by the volume probe.
    mounts: HashMap<String, Vec<String>>,
    last_pass: Option<Instant>,
    buf: Vec<u8>,
}

impl DiskProbe {
    /// The number of the disk called `name`, assigned at first sight: the lowest
    /// not held by an attached disk.
    pub(super) fn number_of(&mut self, name: &str) -> u32 {
        if let Some(&n) = self.numbers.get(name) {
            return n;
        }
        let n = (0..u32::MAX)
            .find(|n| !self.numbers.values().any(|v| v == n))
            .unwrap_or(0);
        self.numbers.insert(name.to_owned(), n);
        n
    }

    /// Tell the probe where each disk is mounted, for its display name. A disk
    /// whose mounts changed is renamed from its next pass.
    pub(super) fn set_mounts(&mut self, mounts: HashMap<String, Vec<String>>) {
        if mounts == self.mounts {
            return;
        }
        self.mounts = mounts;
        for (dev, d) in &mut self.disks {
            let name = name_with_mounts(dev, self.mounts.get(dev));
            if name != d.info.name {
                d.info = Arc::new(DiskInfo {
                    name,
                    ..(*d.info).clone()
                });
            }
        }
    }

    pub(super) fn sample(&mut self, out: &mut Vec<DiskSample>) {
        let now = Instant::now();
        let wall_ms = self
            .last_pass
            .map_or(0.0, |p| now.duration_since(p).as_secs_f64() * 1000.0);
        self.last_pass = Some(now);
        if fs::read_into("/proc/diskstats", &mut self.buf).is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        let mut present = Vec::new();
        for line in text.lines() {
            let Some((name, c)) = parse_line(line) else {
                continue;
            };
            let physical = *self.physical.entry(name.to_owned()).or_insert_with(|| {
                std::path::Path::new(&format!("/sys/block/{name}/device")).exists()
            });
            if !physical {
                continue;
            }
            present.push(name.to_owned());
            if !self.disks.contains_key(name) {
                let number = self.number_of(name);
                let info = Arc::new(read_info(name, number, self.mounts.get(name)));
                self.disks.insert(name.to_owned(), Disk { info, prev: c });
                continue; // No interval to measure yet.
            }
            let d = self.disks.get_mut(name).expect("checked");
            let prev = std::mem::replace(&mut d.prev, c);
            if wall_ms <= 0.0 {
                continue;
            }
            let transfers = (c.reads + c.writes).saturating_sub(prev.reads + prev.writes);
            let busy_ms =
                (c.ms_reading + c.ms_writing).saturating_sub(prev.ms_reading + prev.ms_writing);
            let secs = wall_ms / 1000.0;
            out.push(DiskSample {
                info: Arc::clone(&d.info),
                active: Percent::from_ratio(
                    (c.ms_active.saturating_sub(prev.ms_active) as f64 / wall_ms) as f32,
                )
                .clamped(100.0),
                read_per_sec: Bytes(
                    (c.sectors_read.saturating_sub(prev.sectors_read) as f64 * 512.0 / secs) as u64,
                ),
                write_per_sec: Bytes(
                    (c.sectors_written.saturating_sub(prev.sectors_written) as f64 * 512.0 / secs)
                        as u64,
                ),
                // Task Manager shows 0 ms for an idle disk, not a blank.
                response_ms: Some(if transfers == 0 {
                    0.0
                } else {
                    busy_ms as f32 / transfers as f32
                }),
            });
        }
        // Forget detached disks, freeing their numbers.
        self.disks.retain(|name, _| present.contains(name));
        self.numbers.retain(|name, _| present.contains(name));
        self.physical.retain(|name, _| text.contains(name.as_str()));
        out.sort_by_key(|d| d.info.number);
    }
}

fn read_info(name: &str, number: u32, mounts: Option<&Vec<String>>) -> DiskInfo {
    let dir = format!("/sys/block/{name}");
    let sys_path = std::fs::read_link(&dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let model = fs::read_trimmed(format!("{dir}/device/model"))
        .or_else(|| fs::read_trimmed(format!("{dir}/device/name")));
    DiskInfo {
        number,
        name: name_with_mounts(name, mounts),
        model,
        ssd: fs::read_u64(format!("{dir}/queue/rotational")).map(|r| r == 0),
        capacity: fs::read_u64(format!("{dir}/size")).map(|s| Bytes(s * 512)),
        removable: fs::read_u64(format!("{dir}/removable")) == Some(1),
        bus: bus_of(name, &sys_path),
    }
}

/// `sda (/ /home)`: the kernel's name, then where its volumes are mounted.
fn name_with_mounts(name: &str, mounts: Option<&Vec<String>>) -> String {
    match mounts.filter(|m| !m.is_empty()) {
        Some(m) => format!("{name} ({})", m.join(" ")),
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diskstats_lines_parse() {
        let (name, c) = parse_line(
            "   8       0 sda 1200 30 96000 400 800 20 64000 900 0 1100 1300 0 0 0 0 0 0",
        )
        .unwrap();
        assert_eq!(name, "sda");
        assert_eq!(c.reads, 1200);
        assert_eq!(c.sectors_read, 96000);
        assert_eq!(c.ms_reading, 400);
        assert_eq!(c.writes, 800);
        assert_eq!(c.sectors_written, 64000);
        assert_eq!(c.ms_writing, 900);
        assert_eq!(c.ms_active, 1100);
    }

    #[test]
    fn buses_from_names_and_paths() {
        let bus = |n, p| bus_of(n, p);
        assert_eq!(
            bus(
                "nvme0n1",
                "../devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1"
            )
            .as_deref(),
            Some("NVMe")
        );
        assert_eq!(bus("sdb", "../devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host0/target0:0:0/0:0:0:0/block/sdb").as_deref(), Some("USB"));
        assert_eq!(
            bus(
                "sda",
                "../devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"
            )
            .as_deref(),
            Some("SATA")
        );
        assert_eq!(
            bus(
                "mmcblk0",
                "../devices/platform/mmc0/mmc_host/mmc0/mmc0:0001/block/mmcblk0"
            )
            .as_deref(),
            Some("SD")
        );
    }

    #[test]
    fn numbers_are_reused_lowest_first() {
        let mut p = DiskProbe::default();
        assert_eq!(p.number_of("sda"), 0);
        assert_eq!(p.number_of("sdb"), 1);
        assert_eq!(p.number_of("sda"), 0);
        p.numbers.remove("sda");
        assert_eq!(p.number_of("sdc"), 0);
    }

    #[test]
    fn a_pass_lists_disks() {
        let mut p = DiskProbe::default();
        let mut out = Vec::new();
        p.sample(&mut out);
        std::thread::sleep(std::time::Duration::from_millis(20));
        p.sample(&mut out);
        for d in &out {
            assert!((0.0..=100.0).contains(&d.active.get()));
        }
    }
}
